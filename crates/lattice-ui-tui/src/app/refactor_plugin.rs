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

        // RF.4g: no language server can attach. The builtin configs name
        // whatever `rust-analyzer` is on PATH, and `cre` asks the server
        // first, so a test would race a real server's startup and its answer
        // would depend on the machine. The table is emptied BEFORE the file
        // opens, as peer_conformance's boot does: opening it with a path
        // already set starts the server before anything can stop it.
        let path = base.join(file);
        std::fs::write(&path, text).unwrap();
        let mut app = app_with("\n", 20);
        app.editor
            .lsp
            .set_configs(Vec::new())
            .await
            .expect("the supervisor takes the empty set");
        press_chars(&mut app, &format!(":e {}", path.display()));
        press(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            settle(&mut app, |a| a.editor.document.path() == Some(path.clone())
                && a.editor.document.snapshot().buffer.as_string() == text)
            .await,
            "the harness's file opens"
        );
        if let Some(lang) = lang {
            attach_test_syntax(&mut app, lang);
        }
        let ed = &app.editor;
        let host = Arc::new(
            PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
        );
        let loader = Arc::new(PluginLoader::with_services(
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
                // RF.4: the operators declare chords, which the host wires.
                operator_chords: ed
                    .services
                    .get::<lattice_mode::OperatorChordWirerHandle>()
                    .map(|h| (*h).clone()),
                help_topics: Some(ed.help_topics.clone()),
                ..Default::default()
            },
        ));
        // Production's `install` wires the `<id>.enabled` gate this way; without
        // it `:set refactor.enabled=false` has no listener.
        loader.subscribe_mode_gates();
        let loaded = loader
            .discover_and_load(&base.join("plugins"), TrustTier::Bundled)
            .await;
        assert_eq!(loaded, 1, "the shipped refactor plugin loads");
        assert!(
            settle_mode(&mut app, "refactor-mode").await,
            "`refactor-mode` activates from the manifest's `default_modes`, not by hand"
        );
        // Held as production's `install` holds it: the `<id>.enabled` gate
        // keeps only a `Weak` to the loader, and dies with the last strong one.
        Arc::get_mut(&mut app.editor.services)
            .expect("the harness's service registry is not shared yet")
            .register::<lattice_plugin_loader::PluginLoaderHandle>(loader);
        // And `lsp-mode` off, so a request meets the closed gate and the
        // native rung runs inside the keystroke that asked: the tests read
        // the result straight after the keys. A test of the server rung turns
        // the mode back on (`with_lsp_mode`), with or without a server.
        let buffer = app.editor.document_buffer_id;
        let _ = app
            .editor
            .deactivate_mode_by_id(buffer, lattice_lsp::modes::LspMode::mode_id());
        assert!(!app.editor.lsp_mode_enabled_for(buffer));
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

    fn body(app: &App) -> String {
        app.editor.document.snapshot().buffer.as_string()
    }

    /// The item is the argument, not the identifier inside it: on `z` in
    /// `baz + 1`, `+` separates operands, so the swap moves `baz + 1`. The
    /// `, ` stays where it was, the cursor follows the moved item, and one
    /// `u` takes the whole swap back.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_moves_the_list_item_and_keeps_the_separators() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        submit_ex(&mut app, "refactor-swap-prev");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = foo(baz + 1, bar);\n}\n"
        );
        assert_eq!(
            (app.editor.cursor.line, app.editor.cursor.byte),
            (1, 16),
            "on the moved item"
        );

        submit_ex(&mut app, "refactor-swap-next");
        assert_eq!(body(&app), RUST, "and back");
        assert_eq!((app.editor.cursor.line, app.editor.cursor.byte), (1, 21));

        press_chars(&mut app, "u");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = foo(baz + 1, bar);\n}\n",
            "one `u` undoes one swap"
        );
    }

    /// Statements are items too — separated by a line break — and the
    /// indentation between them stays put.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_moves_a_statement_past_the_next() {
        let src = "fn main() {\n    a();\n    b();\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jw");
        submit_ex(&mut app, "refactor-swap-next");
        assert_eq!(body(&app), "fn main() {\n    b();\n    a();\n}\n");
        assert_eq!((app.editor.cursor.line, app.editor.cursor.byte), (2, 4));
    }

    /// Nothing past the end: the buffer is untouched and the user told why.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_past_the_last_item_says_so_and_changes_nothing() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        submit_ex(&mut app, "refactor-swap-next");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("no next item"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// Half-parsed text is never rearranged: whatever the parser made of the
    /// broken argument, the swap leaves the buffer alone and says why.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_never_rearranges_text_that_does_not_parse() {
        let src = "fn main() {\n    foo(a, $$ ), b);\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfa");
        submit_ex(&mut app, "refactor-swap-next");
        assert_eq!(body(&app), src);
        assert!(
            echoed(&app).starts_with("refactor:"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// Raise replaces the expression around the node. Out of `baz + 1` that
    /// is the sum; out of an argument it is the whole call, not just the
    /// `(…)` — replacing the brackets alone would glue `foo` to the result.
    /// Each raise is one undo step.
    #[tokio::test(flavor = "multi_thread")]
    async fn raise_replaces_the_sum_then_the_call() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        submit_ex(&mut app, "refactor-raise");
        assert_eq!(body(&app), "fn main() {\n    let x = foo(bar, baz);\n}\n");
        assert_eq!((app.editor.cursor.line, app.editor.cursor.byte), (1, 21));

        submit_ex(&mut app, "refactor-raise");
        assert_eq!(body(&app), "fn main() {\n    let x = baz;\n}\n");
        assert_eq!((app.editor.cursor.line, app.editor.cursor.byte), (1, 12));

        press_chars(&mut app, "u");
        assert_eq!(body(&app), "fn main() {\n    let x = foo(bar, baz);\n}\n");
    }

    /// A bracket that is an expression by itself — an array — is what gets
    /// replaced, so the binding keeps its `=`.
    #[tokio::test(flavor = "multi_thread")]
    async fn raise_out_of_an_array_replaces_the_array() {
        let src = "fn main() {\n    let v = [a, b];\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfb");
        submit_ex(&mut app, "refactor-raise");
        assert_eq!(body(&app), "fn main() {\n    let v = b;\n}\n");
    }

    /// A top-level item has only the file around it: refused, not a file
    /// replaced by one function.
    #[tokio::test(flavor = "multi_thread")]
    async fn raise_at_top_level_says_so_and_changes_nothing() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        submit_ex(&mut app, "refactor-raise");
        assert_eq!(body(&app), RUST);
        assert!(echoed(&app).contains("top level"), "got {:?}", echoed(&app));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn raise_works_in_python() {
        let Some(mut app) = app_with_refactor(
            "main.py",
            "x = f(a, b)\n",
            Some(lattice_syntax::Lang::Python),
        )
        .await
        else {
            return;
        };
        press_chars(&mut app, "fb");
        submit_ex(&mut app, "refactor-raise");
        assert_eq!(body(&app), "x = b\n");
    }

    /// Split puts each argument on its own line one indent deeper, the `)`
    /// back at the statement's indent; join is the inverse, from anywhere on
    /// the list. Each is one undo step.
    #[tokio::test(flavor = "multi_thread")]
    async fn split_and_join_round_trip_the_argument_list() {
        let split = "fn main() {\n    let x = foo(\n        bar,\n        baz + 1\n    );\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfb");
        submit_ex(&mut app, "refactor-split");
        assert_eq!(body(&app), split);

        submit_ex(&mut app, "refactor-join");
        assert_eq!(body(&app), RUST);

        press_chars(&mut app, "u");
        assert_eq!(body(&app), split, "one `u` undoes the join");
    }

    /// A trailing comma is the language's style, not the plugin's: kept when
    /// it was there, in both directions.
    #[tokio::test(flavor = "multi_thread")]
    async fn split_and_join_keep_a_trailing_comma() {
        let src = "fn main() {\n    let v = [1, 2,];\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jf1");
        submit_ex(&mut app, "refactor-split");
        assert_eq!(
            body(&app),
            "fn main() {\n    let v = [\n        1,\n        2,\n    ];\n}\n"
        );
        submit_ex(&mut app, "refactor-join");
        assert_eq!(body(&app), src);
    }

    /// The indent is the BUFFER's: a `:setlocal noexpandtab` (what `go-mode`
    /// does for Go) splits with tabs although the global `expandtab` is on.
    /// Proves `get-option-in-buffer` sees buffer-local values.
    #[tokio::test(flavor = "multi_thread")]
    async fn split_indents_with_the_buffer_local_options() {
        let src = "fn main() {\n\tlet x = foo(a, b);\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        submit_ex(&mut app, "setlocal noexpandtab");
        press_chars(&mut app, "jfa");
        submit_ex(&mut app, "refactor-split");
        assert_eq!(
            body(&app),
            "fn main() {\n\tlet x = foo(\n\t\ta,\n\t\tb\n\t);\n}\n"
        );
    }

    /// Only comma lists: a function's parts are not one, and are refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn split_outside_a_comma_list_says_so_and_changes_nothing() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        submit_ex(&mut app, "refactor-split");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("not in a comma-separated list"),
            "got {:?}",
            echoed(&app)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn split_works_in_python() {
        let Some(mut app) = app_with_refactor(
            "main.py",
            "x = f(a, b)\n",
            Some(lattice_syntax::Lang::Python),
        )
        .await
        else {
            return;
        };
        press_chars(&mut app, "fa");
        submit_ex(&mut app, "refactor-split");
        assert_eq!(body(&app), "x = f(\n    a,\n    b\n)\n");
    }

    /// RF.4b: the raise operator over a text object, driven by real keys.
    /// `iw` on `bar` is the identifier; raising it replaces the call (the
    /// `(…)` is glued to `foo`, so the call, not the brackets, is what goes).
    /// Also the production proof of RF.4a's seam: the operator answers only
    /// if the actor route handed it the tree.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_raise_operator_lifts_an_argument_out_of_its_call() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcrriw");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = bar;\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
        press_chars(&mut app, "u");
        assert_eq!(body(&app), RUST, "one `u`");
    }

    /// A range's edges are trimmed of whitespace and separators before the
    /// node is looked for: `vf,` selects `bar,`, which means `bar`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_raise_operator_ignores_a_separator_at_the_range_edge() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbvf, rr");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = bar;\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// In Visual the operator raises the selection: `<M-o>` twice selects
    /// `baz + 1`, `<leader>rr` raises it, and Normal resumes.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_raise_operator_raises_a_visual_selection() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        alt(&mut app, 'o');
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("baz + 1"));
        press_chars(&mut app, " rr");
        assert_eq!(body(&app), "fn main() {\n    let x = baz + 1;\n}\n");
        assert!(matches!(app.editor.modal, ModalState::Normal));
    }

    /// A range that is not one node is refused, not widened: raising more
    /// than was selected would delete text the user did not point at. `i(`
    /// is both arguments.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_raise_operator_refuses_a_range_that_is_not_one_node() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcrri(");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("not one syntax node"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// RF.4c: split and join as operators. `cr<CR>i(` splits the call's
    /// arguments; from an item, `crjiw` joins the list around it —
    /// the nearest list, since split / join change only whitespace.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_split_and_join_operators_act_on_the_list_around_the_range() {
        let split = "fn main() {\n    let x = foo(\n        bar,\n        baz + 1\n    );\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcr");
        enter(&mut app);
        press_chars(&mut app, "i(");
        assert_eq!(body(&app), split, "echo: {:?}", echoed(&app));

        press_chars(&mut app, "j^crjiw");
        assert_eq!(body(&app), RUST, "echo: {:?}", echoed(&app));

        press_chars(&mut app, "u");
        assert_eq!(body(&app), split, "one `u` undoes the join");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_split_operator_outside_a_comma_list_says_so() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "cr");
        enter(&mut app);
        press_chars(&mut app, "iw");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("not in a comma-separated list"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// The text each published range tint covers — what both renderers
    /// paint from `mode_highlights`.
    fn tinted(app: &App) -> Vec<String> {
        let text = app.editor.document.snapshot().buffer.as_string();
        let offset = |p: lattice_protocol::position::Position| -> usize {
            text.split_inclusive('\n')
                .take(p.line as usize)
                .map(str::len)
                .sum::<usize>()
                + p.byte as usize
        };
        app.ad()
            .mode_highlights
            .iter()
            .map(|h| text[offset(h.range.start)..offset(h.range.end)].to_string())
            .collect()
    }

    fn enter(app: &mut App) {
        press(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    fn esc(app: &mut App) {
        press(app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }

    /// RF.4d: `cx{motion}` twice swaps the two ranges in one undo step. The
    /// first marks, and the mark is tinted at once — not on the next key.
    #[tokio::test(flavor = "multi_thread")]
    async fn exchange_swaps_two_ranges_in_one_undo_step() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcxiw");
        assert_eq!(body(&app), RUST, "the first `cx` only marks");
        assert_eq!(tinted(&app), vec!["bar"], "and the mark shows");

        press_chars(&mut app, "fzcxiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = foo(baz, bar + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
        assert!(tinted(&app).is_empty(), "the tint goes with the mark");

        press_chars(&mut app, "u");
        assert_eq!(body(&app), RUST, "one `u`");
    }

    /// The mark is the document's: an edit elsewhere moves it, and the swap
    /// still lands on the text that was marked.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_exchange_mark_follows_an_edit_before_it() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfzcxiw0iY");
        esc(&mut app);
        assert_eq!(tinted(&app), vec!["baz"], "the tint moved with the text");
        press_chars(&mut app, "fbcxiw");
        assert_eq!(
            body(&app),
            "fn main() {\nY    let x = foo(baz, bar + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// An edit to the marked text drops the mark and says so, rather than
    /// swapping text the user no longer marked.
    #[tokio::test(flavor = "multi_thread")]
    async fn editing_the_marked_text_drops_the_mark() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcxiwxfzcxiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = foo(ar, baz + 1);\n}\n"
        );
        assert!(
            echoed(&app).contains("mark was dropped"),
            "got {:?}",
            echoed(&app)
        );
        assert!(tinted(&app).is_empty());
    }

    /// `cxc` forgets the mark, so the next `cx` marks afresh.
    #[tokio::test(flavor = "multi_thread")]
    async fn cxc_clears_the_exchange_mark() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcxiwcxc");
        assert!(tinted(&app).is_empty(), "cleared");
        press_chars(&mut app, "fzcxiw");
        assert_eq!(body(&app), RUST, "a fresh mark, not a swap");
        assert_eq!(tinted(&app), vec!["baz"]);
    }

    /// In Visual, `X` exchanges the selection — the same operator (`cx`
    /// cannot: Visual `c` changes the selection at once).
    #[tokio::test(flavor = "multi_thread")]
    async fn exchange_in_visual_uses_the_selection() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbviwX");
        assert_eq!(tinted(&app), vec!["bar"]);
        press_chars(&mut app, "fzviwX");
        assert_eq!(
            body(&app),
            "fn main() {\n    let x = foo(baz, bar + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// `cxx` is the line: two `cxx`s swap two lines, the break between them
    /// kept.
    #[tokio::test(flavor = "multi_thread")]
    async fn cxx_exchanges_lines() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jcxxkcxx");
        assert_eq!(
            body(&app),
            "    let x = foo(bar, baz + 1);\nfn main() {\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// RF.4e: Visual `<leader>re` over a selected expression declares it before
    /// its statement and leaves the name selected, linked at both sites:
    /// typing it once names both. Undo takes it back in one step.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_declares_the_expression_and_links_the_name() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        alt(&mut app, 'o');
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("baz + 1"));
        press_chars(&mut app, " re");
        assert_eq!(
            body(&app),
            "fn main() {\n    let value = baz + 1;\n    let x = foo(bar, value);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
        press_chars(&mut app, "sum");
        esc(&mut app);
        assert_eq!(
            body(&app),
            "fn main() {\n    let sum = baz + 1;\n    let x = foo(bar, sum);\n}\n",
            "the name typed once lands at both sites"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extract_is_one_undo_step() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcreiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    let value = bar;\n    let x = foo(value, baz + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
        esc(&mut app);
        press_chars(&mut app, "u");
        assert_eq!(body(&app), RUST);
    }

    /// A range that is not one expression is refused, not widened.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_refuses_a_range_that_is_not_one_expression() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcrei(");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("not one expression"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// Python declares by assignment, at the statement's own indent.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_in_python_assigns() {
        let src = "def f():\n    x = foo(bar, baz + 1)\n";
        let Some(mut app) =
            app_with_refactor("main.py", src, Some(lattice_syntax::Lang::Python)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbcreiw");
        assert_eq!(
            body(&app),
            "def f():\n    value = bar\n    x = foo(value, baz + 1)\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// RF.4i: every `refactor.scm` the plugin ships compiles against the
    /// grammar the editor pins. A plugin compiles its query only when a
    /// refactoring first runs in that language, so an error would otherwise
    /// surface as one user's refusal, not a red test.
    #[test]
    fn every_refactor_query_compiles_against_the_pinned_grammar() {
        let registry = lattice_syntax::registry::LangRegistry::standard().unwrap();
        for (lang, dir) in [
            ("rust", "rust"),
            ("python", "python"),
            ("ruby", "ruby"),
            ("go", "go"),
            ("javascript", "javascript"),
            ("typescript", "typescript"),
            ("tsx", "typescript"),
            ("cpp", "cpp"),
            ("java", "java"),
            ("lua", "lua"),
        ] {
            let path = format!("{PLUGIN_DIR}/queries/{dir}/refactor.scm");
            let source = std::fs::read_to_string(&path).unwrap();
            let language = registry.tree_sitter_language(lang).unwrap();
            if let Err(e) = tree_sitter::Query::new(&language, &source) {
                panic!("{lang}: {path}: {e}");
            }
        }
    }

    /// RF.4i: run `cre{motion}` over `src` with the caret at the `q` on
    /// line `line`; the buffer afterwards and the echo.
    async fn extract_q(
        file: &str,
        src: &str,
        lang: lattice_syntax::Lang,
        line: usize,
    ) -> Option<(String, String)> {
        let mut app = app_with_refactor(file, src, Some(lang)).await?;
        press_chars(&mut app, &format!("{}fqcreiw", "j".repeat(line)));
        Some((body(&app), echoed(&app)))
    }

    /// RF.4i: an expression in one `match` arm runs only when that arm is
    /// taken. RF.4e's text rule declared it inside the arm; the query facts
    /// refuse it, naming why.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_out_of_a_match_arm_is_refused() {
        let src = "fn main() {\n    let r = match k {\n        1 => foo(q),\n        _ => 0,\n    };\n}\n";
        let Some((after, echo)) = extract_q("main.rs", src, lattice_syntax::Lang::Rust, 2).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("branch"), "got {echo:?}");
    }

    /// RF.4i: the right of `&&` does not always run, so it is not lifted.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_from_the_right_of_a_short_circuit_is_refused() {
        let src = "fn main() {\n    let ok = a && good(q);\n}\n";
        let Some((after, echo)) = extract_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("short-circuit"), "got {echo:?}");
    }

    /// RF.4i: a closure with a block body has statements of its own, so the
    /// declaration goes inside it, at the inner indent.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_inside_a_closure_block_declares_inside_it() {
        let src = "fn main() {\n    let f = |a| {\n        go(q)\n    };\n}\n";
        let Some((after, echo)) = extract_q("main.rs", src, lattice_syntax::Lang::Rust, 2).await
        else {
            return;
        };
        assert_eq!(
            after,
            "fn main() {\n    let f = |a| {\n        let value = q;\n        go(value)\n    };\n}\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4i: `v` is defined in the `if`'s own initializer, so lifting the
    /// condition above the `if` would read it before it exists. Read from
    /// `locals.scm` through `language-query`.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_that_reads_a_name_defined_earlier_in_its_statement_is_refused() {
        let src = "package main\n\nfunc main() {\n\tif v := f(); v > q {\n\t}\n}\n";
        let Some((after, echo)) = extract_q("main.go", src, lattice_syntax::Lang::Go, 3).await
        else {
            return;
        };
        // `q` alone reads nothing defined there: it is extracted.
        assert!(
            after.contains("value := q\n\tif v := f(); v > value"),
            "{after:?} / {echo:?}"
        );
        let mut app = app_with_refactor("main.go", src, Some(lattice_syntax::Lang::Go))
            .await
            .expect("built above");
        // `v > q`: the comparison reads `v`.
        press_chars(&mut app, "jjjfv;v");
        press_chars(&mut app, "fq re");
        assert_eq!(body(&app), src);
        assert!(echoed(&app).contains("`v`"), "got {:?}", echoed(&app));
    }

    /// RF.4i: one side of a conditional expression runs, not both.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_from_a_ternary_branch_is_refused() {
        let src = "def f():\n    x = a if c else g(q)\n";
        let Some((after, echo)) = extract_q("main.py", src, lattice_syntax::Lang::Python, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("branch"), "got {echo:?}");
    }

    /// RF.4i: a loop's condition runs every time round, not once.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_from_a_loop_condition_is_refused() {
        let src = "fn main() {\n    while go(q) {\n    }\n}\n";
        let Some((after, echo)) = extract_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("loop"), "got {echo:?}");
    }

    /// RF.4i: an arrow function's expression body runs when it is called.
    /// It has no statements of its own, so there is nowhere inside to go.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_from_an_expression_bodied_closure_is_refused() {
        let src = "function m() {\n  const f = (a) => g(q);\n}\n";
        let Some((after, echo)) =
            extract_q("main.ts", src, lattice_syntax::Lang::TypeScript, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("closure"), "got {echo:?}");
    }

    /// RF.4i: a comprehension's body runs once per item.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_from_a_comprehension_is_refused() {
        let src = "def f():\n    xs = [g(q) for y in ys]\n";
        let Some((after, echo)) = extract_q("main.py", src, lattice_syntax::Lang::Python, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("comprehension"), "got {echo:?}");
    }

    /// `refactor.declaration`, when set, replaces the language's form — how a
    /// user gives extract a language the table does not know. Set with `:set`:
    /// a plugin option cannot be `:setlocal`ed yet (buffer-local overrides are
    /// keyed by a Rust type a plugin option does not have — RF.4e′). Extract
    /// reads it with `get-option-in-buffer`, so it turns buffer-local with no
    /// change to the plugin once that gap closes.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_follows_the_declaration_option() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(
            &mut app,
            ":set refactor.declaration=const {name}: i32 = {value};",
        );
        press(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        press_chars(&mut app, "jfbcreiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    const value: i32 = bar;\n    let x = foo(value, baz + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// RF.4g: set an option the way a user does.
    fn set(app: &mut App, assignment: &str) {
        press_chars(app, &format!(":set {assignment}"));
        enter(app);
    }

    /// RF.4g: `refactor.source=native` skips the server altogether, so the
    /// native extract runs from the operator itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_with_source_native_does_not_ask_the_server() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        set(&mut app, "refactor.source=native");
        press_chars(&mut app, "jfbcreiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    let value = bar;\n    let x = foo(value, baz + 1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// RF.4g: `refactor.source=lsp` names no fallback, so with no server
    /// nothing is extracted and the request says why.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_with_source_lsp_and_no_server_changes_nothing() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        set(&mut app, "refactor.source=lsp");
        press_chars(&mut app, "jfbcreiw");
        assert_eq!(body(&app), RUST);
        assert!(
            echoed(&app).contains("lsp-mode disabled"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// RF.4f: run `criiw` with the caret on the `q` on line `line`; the buffer
    /// afterwards and the echo.
    async fn inline_q(
        file: &str,
        src: &str,
        lang: lattice_syntax::Lang,
        line: usize,
    ) -> Option<(String, String)> {
        let mut app = app_with_refactor(file, src, Some(lang)).await?;
        press_chars(&mut app, &format!("{}fqcriiw", "j".repeat(line)));
        Some((body(&app), echoed(&app)))
    }

    /// RF.4f: with no server, `cri` inlines natively. From the declaration
    /// or from a use (Visual `<leader>ri`), it is the same edit, and one
    /// undo step.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_replaces_every_use_and_deletes_the_declaration() {
        let src = "fn main() {\n    let q = 1;\n    let b = q + 2;\n    g(q);\n}\n";
        let want = "fn main() {\n    let b = 1 + 2;\n    g(1);\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, want, "echo: {echo:?}");

        let mut app = app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust))
            .await
            .expect("built above");
        press_chars(&mut app, "jjjfqviw ri");
        assert_eq!(body(&app), want, "echo: {:?}", echoed(&app));
        esc(&mut app);
        press_chars(&mut app, "u");
        assert_eq!(body(&app), src, "one undo step");
    }

    /// RF.4f: `refactor.source=native` inlines from the operator itself,
    /// with no request to a server in between.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_with_source_native_does_not_ask_the_server() {
        let src = "fn main() {\n    let q = 1;\n    g(q);\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        set(&mut app, "refactor.source=native");
        press_chars(&mut app, "jfqcriiw");
        assert_eq!(
            body(&app),
            "fn main() {\n    g(1);\n}\n",
            "echo: {:?}",
            echoed(&app)
        );
    }

    /// RF.4f: an operator value is parenthesised where an operand goes, and
    /// left bare as a whole argument.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_parenthesises_an_operator_value_only_where_it_binds() {
        let src = "fn main() {\n    let q = x + 1;\n    g(q, q * 2);\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(
            after, "fn main() {\n    g(x + 1, (x + 1) * 2);\n}\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4f: a call would run once per use.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_of_a_call_used_twice_is_refused() {
        let src = "fn main() {\n    let q = f();\n    g(q);\n    h(q);\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("2 times"), "got {echo:?}");
    }

    /// RF.4f: a call used once moves into a loop, where it would run every
    /// time round.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_of_a_call_into_a_loop_is_refused() {
        let src = "fn main() {\n    let q = f();\n    while go() {\n        g(q);\n    }\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("loop"), "got {echo:?}");
    }

    /// RF.4f: `S { q }` names the field too, so it becomes `S { q: value }`.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_into_a_field_shorthand_keeps_the_field() {
        let src = "fn main() {\n    let q = 1;\n    let s = S { q };\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(
            after, "fn main() {\n    let s = S { q: 1 };\n}\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4f: `"{q}"` reads `q` with no node for it, so inline would miss it.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_with_a_use_in_a_format_string_is_refused() {
        let src = "fn main() {\n    let q = 1;\n    println!(\"{q}\");\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("format string"), "got {echo:?}");
    }

    /// RF.4f: a second `let` shadows rather than assigns, so the first
    /// inlines into the uses before it.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_stops_at_a_shadowing_declaration() {
        let src = "fn main() {\n    let q = 1;\n    let q = q + 1;\n    g(q);\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(
            after, "fn main() {\n    let q = 1 + 1;\n    g(q);\n}\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4f: `let mut` is not one value.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_of_a_mutable_binding_is_refused() {
        let src = "fn main() {\n    let mut q = 1;\n    g(q);\n}\n";
        let Some((after, echo)) = inline_q("main.rs", src, lattice_syntax::Lang::Rust, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("not a variable declared"), "got {echo:?}");
    }

    /// RF.4f: in Python, binding is assignment, so a second `q =` in the
    /// same function means the uses read different values.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_in_python_refuses_a_reassigned_name() {
        let src = "def f():\n    q = 1\n    g(q)\n    q = 2\n";
        let Some((after, echo)) = inline_q("main.py", src, lattice_syntax::Lang::Python, 1).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(echo.contains("assigned again"), "got {echo:?}");
        let ok = "def f():\n    q = a + 1\n    return q * 2\n";
        let Some((after, echo)) = inline_q("main.py", ok, lattice_syntax::Lang::Python, 1).await
        else {
            return;
        };
        assert_eq!(
            after, "def f():\n    return (a + 1) * 2\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4f: JavaScript's `{ q }` keeps the property name.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_in_javascript_keeps_a_shorthand_property() {
        let src = "function m() {\n  const q = 1;\n  return { q };\n}\n";
        let Some((after, echo)) =
            inline_q("main.js", src, lattice_syntax::Lang::JavaScript, 1).await
        else {
            return;
        };
        assert_eq!(
            after, "function m() {\n  return { q: 1 };\n}\n",
            "echo: {echo:?}"
        );
    }

    /// RF.4f: the value reads `y`, which changes before the use.
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_in_go_refuses_a_value_whose_input_changes() {
        let src = "package main\n\nfunc main() {\n\ty := 1\n\tq := y\n\ty++\n\tg(q)\n}\n";
        let Some((after, echo)) = inline_q("main.go", src, lattice_syntax::Lang::Go, 4).await
        else {
            return;
        };
        assert_eq!(after, src);
        assert!(
            echo.contains("`y`") && echo.contains("assigned after"),
            "got {echo:?}"
        );
    }

    /// An in-process language server's runtime, shut down without blocking
    /// so a failing assertion inside the async test does not abort.
    struct MockServer(Option<tokio::runtime::Runtime>);

    impl Drop for MockServer {
        fn drop(&mut self) {
            if let Some(rt) = self.0.take() {
                rt.shutdown_background();
            }
        }
    }

    /// RF.4g: `lsp-mode` back on, with no server in the table: what a
    /// machine without the language's server has.
    async fn with_lsp_mode(app: &mut App) {
        let buffer = app.editor.document_buffer_id;
        let _ = app
            .editor
            .activate_mode_by_id(buffer, lattice_lsp::modes::LspMode::mode_id());
        assert!(settle_mode(app, "lsp-code-action-mode").await);
        let uri = app.editor.buffer_uris.get(&buffer).cloned().expect("a uri");
        assert!(
            app.editor.lsp.servers_for(&uri).is_empty(),
            "no real server"
        );
    }

    /// RF.4g: `lsp-mode` back on (with no server in the table) and an
    /// in-process server attached that answers `textDocument/codeAction`
    /// with `actions` and advertises `kinds`.
    async fn with_server(
        app: &mut App,
        kinds: serde_json::Value,
        actions: serde_json::Value,
    ) -> (MockServer, lattice_host::peer_conformance::MockLspLog) {
        with_lsp_mode(app).await;
        let path = app
            .editor
            .document
            .path()
            .expect("the harness opens a file")
            .to_path_buf();
        let serve: lattice_host::peer_conformance::MockLspServe =
            Arc::new(move |method, _params| match method {
                "textDocument/codeAction" => (actions.clone(), std::time::Duration::ZERO),
                _ => (serde_json::Value::Null, std::time::Duration::ZERO),
            });
        let (rt, log) = tokio::task::block_in_place(|| {
            lattice_host::peer_conformance::attach_mock_lsp_serving(
                app,
                &path,
                serde_json::json!({ "codeActionProvider": { "codeActionKinds": kinds } }),
                serve,
            )
        });
        (MockServer(Some(rt)), log)
    }

    /// The `textDocument/codeAction` requests a mock server received.
    fn code_action_requests(
        log: &lattice_host::peer_conformance::MockLspLog,
    ) -> Vec<serde_json::Value> {
        log.lock()
            .expect("log")
            .iter()
            .filter(|(method, _)| method == "textDocument/codeAction")
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// RF.3c: the `:` forms moved into the plugin. With a server,
    /// `:'<,'>refactor-extract` asks for extractions over exactly what was
    /// selected, and `:lsp-refactor` for every refactoring at the cursor.
    #[tokio::test(flavor = "multi_thread")]
    async fn ex_commands_ask_the_server_with_their_kind_and_range() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        let (_server, log) = with_server(
            &mut app,
            serde_json::json!(["refactor", "refactor.extract"]),
            serde_json::json!([{ "title": "Extract", "kind": "refactor.extract" }]),
        )
        .await;
        press_chars(&mut app, "jfzhhvf1:refactor-extract");
        enter(&mut app);
        assert!(
            settle_woken(&mut app, |_| !code_action_requests(&log).is_empty()).await,
            "a request went out; echo {:?}",
            echoed(&app)
        );
        let first = code_action_requests(&log)[0].clone();
        assert_eq!(
            first["context"]["only"],
            serde_json::json!(["refactor.extract"])
        );
        assert_eq!(
            first["range"],
            serde_json::json!({
                "start": { "line": 1, "character": 21 },
                "end": { "line": 1, "character": 28 }
            }),
            "`baz + 1`, exactly"
        );
        assert_eq!(body(&app), RUST, "the server answered: native did not run");
        esc(&mut app);
        press_chars(&mut app, ":lsp-refactor");
        enter(&mut app);
        assert!(
            settle_woken(&mut app, |_| code_action_requests(&log).len() == 2).await,
            "a second request; echo {:?}",
            echoed(&app)
        );
        assert_eq!(
            code_action_requests(&log)[1]["context"]["only"],
            serde_json::json!(["refactor"])
        );
    }

    /// RF.3c: without a server, `:'<,'>refactor-extract` extracts what was
    /// selected natively, as `cre` does.
    #[tokio::test(flavor = "multi_thread")]
    async fn ex_extract_over_the_selection_runs_natively_without_a_server() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfzhhvf1:refactor-extract");
        enter(&mut app);
        let want = "fn main() {\n    let value = baz + 1;\n    let x = foo(bar, value);\n}\n";
        assert!(
            settle_woken(&mut app, |a| body(a) == want).await,
            "got {:?}; echo {:?}",
            body(&app),
            echoed(&app)
        );
    }

    /// RF.3c: without a server, `:refactor-inline` inlines the variable at
    /// the cursor natively, as `cri` does.
    #[tokio::test(flavor = "multi_thread")]
    async fn ex_inline_at_the_cursor_runs_natively_without_a_server() {
        let src = "fn main() {\n    let q = 1;\n    let b = q + 2;\n    g(q);\n}\n";
        let want = "fn main() {\n    let b = 1 + 2;\n    g(1);\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", src, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jjfq:refactor-inline");
        enter(&mut app);
        assert!(
            settle_woken(&mut app, |a| body(a) == want).await,
            "got {:?}; echo {:?}",
            body(&app),
            echoed(&app)
        );
    }

    /// RF.3c: `:'<,'>lsp-code-action` asks about what was selected, not
    /// the lines around it. `execute_ex_line` makes `'<,'>` whole lines
    /// before dispatch, which left the command unable to tell it had been
    /// given the selection, so it asked about the cursor; the exact
    /// selection now rides beside the lines (`ExCommandContext::selection`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selection_range_asks_the_server_about_the_selection() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        let (_server, log) = with_server(
            &mut app,
            serde_json::json!(["refactor.extract"]),
            serde_json::json!([]),
        )
        .await;
        // `baz + 1`: bytes 21..=27 on line 1.
        press_chars(&mut app, "jfzhhvf1:lsp-code-action");
        enter(&mut app);
        assert!(
            settle_woken(&mut app, |_| !code_action_requests(&log).is_empty()).await,
            "a request went out; echo {:?}",
            echoed(&app)
        );
        assert_eq!(
            code_action_requests(&log)[0]["range"],
            serde_json::json!({
                "start": { "line": 1, "character": 21 },
                "end": { "line": 1, "character": 28 }
            })
        );
    }

    /// RF.4g: with a server, `cre` asks it for extractions — that kind only,
    /// over the operator's own range — and its answer opens as a picker,
    /// with no key pressed after the chord. Native extract does not run.
    #[tokio::test(flavor = "multi_thread")]
    async fn extract_asks_the_server_first() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        let (_server, log) = with_server(
            &mut app,
            serde_json::json!(["quickfix", "refactor.extract"]),
            serde_json::json!([
                { "title": "Extract into variable", "kind": "refactor.extract.variable" }
            ]),
        )
        .await;
        press_chars(&mut app, "jfbcreiw");
        assert!(
            settle_woken(&mut app, |a| a.editor.picker.is_some()).await,
            "the server's extraction opens as a picker; echo {:?}",
            echoed(&app)
        );
        assert_eq!(body(&app), RUST, "native extract did not run");
        let requests = code_action_requests(&log);
        assert_eq!(requests.len(), 1, "one request: {requests:?}");
        let params = &requests[0];
        assert_eq!(
            params["context"]["only"],
            serde_json::json!(["refactor.extract"])
        );
        // `iw` on `bar`: line 1, bytes 16..19 — the range the operator was
        // handed, not the cursor's.
        assert_eq!(
            params["range"],
            serde_json::json!({
                "start": { "line": 1, "character": 16 },
                "end": { "line": 1, "character": 19 }
            })
        );
    }

    /// RF.4g: `lsp-mode` on and no server for the language — the common case
    /// — asks, learns there is none, and runs native extract off that wake,
    /// with no key pressed after the chord.
    #[tokio::test(flavor = "multi_thread")]
    async fn with_no_server_extract_falls_back_off_the_wake() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        with_lsp_mode(&mut app).await;
        press_chars(&mut app, "jfbcreiw");
        let extracted = "fn main() {\n    let value = bar;\n    let x = foo(value, baz + 1);\n}\n";
        assert!(
            settle_woken(&mut app, |a| body(a) == extracted).await,
            "body {:?}, echo {:?}",
            body(&app),
            echoed(&app)
        );
    }

    /// RF.4g: a server with nothing to offer hands over to native extract,
    /// off the wake that carries its answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_server_answer_falls_back_to_native() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        let (_server, log) = with_server(
            &mut app,
            serde_json::json!(["refactor.extract"]),
            serde_json::json!([]),
        )
        .await;
        press_chars(&mut app, "jfbcreiw");
        let extracted = "fn main() {\n    let value = bar;\n    let x = foo(value, baz + 1);\n}\n";
        assert!(
            settle_woken(&mut app, |a| body(a) == extracted).await,
            "body {:?}, echo {:?}",
            body(&app),
            echoed(&app)
        );
        assert_eq!(code_action_requests(&log).len(), 1);
    }

    /// RF.4g: a server whose advertised kinds leave out extract is not asked
    /// at all, and native extract runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_without_extractions_is_not_asked() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        let (_server, log) = with_server(
            &mut app,
            serde_json::json!(["quickfix"]),
            serde_json::json!([{ "title": "never asked", "kind": "refactor.extract" }]),
        )
        .await;
        press_chars(&mut app, "jfbcreiw");
        let extracted = "fn main() {\n    let value = bar;\n    let x = foo(value, baz + 1);\n}\n";
        assert!(
            settle_woken(&mut app, |a| body(a) == extracted).await,
            "body {:?}, echo {:?}",
            body(&app),
            echoed(&app)
        );
        assert!(code_action_requests(&log).is_empty());
    }

    /// RF.4j: the Visual forms are `refactor-mode`'s own `<leader>r` bindings,
    /// the same letter as the Normal `cr` chord: split and join over a
    /// selection.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_visual_forms_live_on_leader_r() {
        let split = "fn main() {\n    let x = foo(\n        bar,\n        baz + 1\n    );\n}\n";
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbviw r");
        enter(&mut app);
        assert_eq!(body(&app), split, "echo: {:?}", echoed(&app));
        assert!(matches!(app.editor.modal, ModalState::Normal));
        press_chars(&mut app, "jwviw rj");
        assert_eq!(body(&app), RUST, "echo: {:?}", echoed(&app));
    }

    /// RF.4j: no `c…` chord is bound in Visual, so Visual `c` still changes
    /// the selection at once — it does not wait for a second key.
    #[tokio::test(flavor = "multi_thread")]
    async fn visual_c_still_changes_at_once() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfbviwc");
        assert!(
            matches!(app.editor.modal, ModalState::Insert),
            "got {:?}",
            app.editor.modal
        );
        assert_eq!(body(&app), "fn main() {\n    let x = foo(, baz + 1);\n}\n");
    }

    /// RF.4j: the chords are `refactor-mode`'s. Turned off, `cre` is plain
    /// vim again — `c` with `r`, which is not a motion, so nothing — and the
    /// Visual `<leader>re` is gone with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_cr_keys_go_with_the_mode() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, ":set refactor.enabled=false");
        enter(&mut app);
        let id = lattice_mode::ModeId::new("refactor-mode");
        assert!(
            settle(&mut app, |a| {
                !a.editor
                    .active_modes
                    .get(&a.editor.document_buffer_id)
                    .is_some_and(|m| m.is_active(id))
            })
            .await,
            "`refactor-mode` leaves the buffer"
        );
        press_chars(&mut app, "jfbcreiw");
        esc(&mut app);
        assert!(
            !body(&app).contains("let value"),
            "extract ran with the mode off: {:?}",
            body(&app)
        );
        press_chars(&mut app, "u0fbviw re");
        esc(&mut app);
        assert!(
            !body(&app).contains("let value"),
            "Visual `<leader>re` ran with the mode off: {:?}",
            body(&app)
        );
    }

    /// The same rule in Python.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_works_in_python() {
        let Some(mut app) = app_with_refactor(
            "main.py",
            "x = f(a, b)\n",
            Some(lattice_syntax::Lang::Python),
        )
        .await
        else {
            return;
        };
        press_chars(&mut app, "fa");
        submit_ex(&mut app, "refactor-swap-next");
        assert_eq!(body(&app), "x = f(b, a)\n");
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
