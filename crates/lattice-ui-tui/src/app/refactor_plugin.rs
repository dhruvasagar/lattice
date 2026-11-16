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
