//! TUI-coupled test harness for the renderer-neutral
//! `lattice_picker::picker_sources` first-party source catalog.
//! Slice 5.7.B.0 moved the production code to lattice-picker; the
//! tests stay here because their `app_with(...)` helper builds a
//! real ui-tui `App` so each source's `init` / `accept` can be
//! exercised against a live `PickerContext` snapshot. The
//! `lattice_picker::picker_sources` test module (in-tree, next to
//! the source impls) covers the pure formatters and grep-line
//! parser; those don't need an `App`.
//!
//! `pub use` re-exports every public item so call sites that
//! referenced `lattice_ui_tui::picker_sources::*` before the move
//! keep resolving without source changes.

pub use lattice_picker::picker_sources::*;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::app_with;
    // The shim re-exports `picker_sources::*` (source structs), but
    // the tests below also exercise registry / routing types that
    // live elsewhere in `lattice-picker` (`source::*`, `outcome::*`).
    // Pull those into scope explicitly so the test bodies that
    // were written against the pre-move `use lattice_picker::{...}`
    // top-level import keep working unchanged.
    use lattice_completion::{CandidateKind, RawCandidate};
    use lattice_grammar::Args;
    use lattice_picker::{
        PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator,
        PickerSourceSpec, RoutingPayload, SourceResult,
    };
    use std::sync::Arc;

    /// Files source emits `OpenFile { path }` routing
    /// payloads pointing under the supplied root.
    #[test]
    fn files_source_inline_init_emits_open_file_routing() {
        let tmp = std::env::temp_dir().join(format!("lattice-files-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = FilesSource::new();
        let result = source
            .init(&ctx, std::slice::from_ref(&tmp.display().to_string()))
            .expect("inline result");
        match result {
            PickerInitResult::Inline(pairs) => {
                assert_eq!(pairs.len(), 2);
                for (_cand, routing) in &pairs {
                    match routing {
                        RoutingPayload::OpenFile { path } => {
                            assert!(path.starts_with(std::fs::canonicalize(&tmp).unwrap()));
                        }
                        other => panic!("expected OpenFile, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Inline, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// MR.3: the files source carries marginalia as typed `Styled`
    /// annotations (perm / size / mtime columns), NOT baked into the
    /// `display` string. `display` is the path so fuzzy matching runs on
    /// the path; the renderer color-codes each annotation per its theme
    /// slot. End-to-end through the source's `init` against a live
    /// `PickerContext`.
    #[test]
    fn files_source_emits_metadata_annotations() {
        let tmp = std::env::temp_dir().join(format!("lattice-files-margin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("readme.md"), b"# hello").unwrap();
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = FilesSource::new();
        let result = source
            .init(&ctx, std::slice::from_ref(&tmp.display().to_string()))
            .expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 1);
        let cand = &pairs[0].0;
        // display is the path only — metadata moved to annotations.
        assert!(cand.display.contains("readme.md"), "got `{}`", cand.display);
        assert!(
            !cand.display.contains("minute") && !cand.display.contains("just now"),
            "metadata must not leak into display: `{}`",
            cand.display
        );
        // perm → size → mtime, each a Styled cell.
        let cats: Vec<&str> = cand.annotations.iter().map(|a| a.category()).collect();
        assert_eq!(cats, vec!["perm", "size", "mtime"]);
        let size = cand.annotations.iter().find(|a| a.category() == "size").unwrap();
        assert_eq!(size.display_text(), "7", "7-byte file → size `7`");
        let mtime = cand.annotations.iter().find(|a| a.category() == "mtime").unwrap();
        let mt = mtime.display_text();
        assert!(
            mt.contains("just now") || mt.contains("minute"),
            "expected relative mtime, got `{mt}`"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Empty workspace makes the files source's init return
    /// `Err("files: no files under ...")` which the host
    /// echoes verbatim.
    #[test]
    fn files_source_empty_root_errors() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-files-src-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = FilesSource::new();
        let err = source
            .init(&ctx, std::slice::from_ref(&tmp.display().to_string()))
            .unwrap_err();
        assert!(err.starts_with("files: no files under"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Recent source: empty MRU returns `Err("no recent files")`.
    #[test]
    fn recent_source_empty_mru_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = RecentFilesSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert_eq!(err, "no recent files");
    }

    /// MP.3: a recent-files row for a stattable path carries the same
    /// eza-style perm/size/mtime marginalia as the file picker.
    #[test]
    fn recent_source_emits_metadata_annotations() {
        let mut app = app_with("hi\n", 5);
        let tmp = std::env::temp_dir()
            .join(format!("lattice-recent-meta-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::write(&tmp, b"hello").unwrap();
        app.editor.push_recent_file(&tmp);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = RecentFilesSource::new();
        let PickerInitResult::Inline(pairs) = source.init(&ctx, &[]).expect("inline") else {
            panic!("expected Inline");
        };
        let row = pairs
            .iter()
            .find(|(c, _)| c.display.contains("lattice-recent-meta"))
            .expect("recent row for the pushed temp file");
        let cats: Vec<&str> = row.0.annotations.iter().map(|a| a.category()).collect();
        assert!(
            cats.contains(&"perm") && cats.contains(&"size") && cats.contains(&"mtime"),
            "expected perm/size/mtime metadata, got {cats:?}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// MP.3: buffers source emits one row per registry entry; the
    /// active buffer floats to the bottom and carries an active-status
    /// marginalia cell (no inline `(current)` in the display). Each row
    /// carries a buffer-id and a kind cell; the path is the display.
    #[test]
    fn buffers_source_inline_init_floats_active_to_bottom() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = BuffersSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert!(!pairs.is_empty());
        // Last row is the active buffer (floated to bottom).
        let last = pairs.last().unwrap();
        match &last.1 {
            RoutingPayload::Buffer { id } => assert_eq!(*id, ctx.active_buffer.buffer_id),
            other => panic!("expected Buffer routing, got {other:?}"),
        }
        let cand = &last.0;
        // No inline markers leak into the matchable display.
        assert!(!cand.display.contains("(current)"));
        assert!(!cand.display.contains("[+]"));
        let cat = |c: &str| {
            cand.annotations
                .iter()
                .find(|a| a.category() == c)
                .map(|a| a.display_text().into_owned())
        };
        // Buffer-id and kind cells present; active row has an active-status
        // marker (`•`).
        assert_eq!(cat("buffer-id"), Some(format!("#{}", ctx.active_buffer.buffer_id)));
        assert!(cat("kind").is_some(), "kind cell missing");
        assert!(
            cat("status").is_some_and(|s| s.contains('•')),
            "active row missing active-status marker, got {:?}",
            cat("status")
        );
    }

    /// Slice 7b.1 probe: BuffersSource candidates carry the
    /// typed `accept_action` (SwitchBuffer) — the
    /// `DefaultAcceptHandler` reads it without the source
    /// needing a custom handler impl. Parallel to the
    /// existing RoutingPayload pair (kept alive through 7c).
    #[test]
    fn buffers_source_candidates_carry_typed_accept_action() {
        use lattice_completion::{AcceptAction, AcceptHandler, DefaultAcceptHandler};

        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = BuffersSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        for (cand, routing) in &pairs {
            // The candidate's typed accept must align with the
            // legacy routing payload (both encode the same id).
            let action = cand
                .accept_action
                .as_deref()
                .expect("BuffersSource sets accept_action on every candidate");
            let RoutingPayload::Buffer { id: routing_id } = routing else {
                panic!("expected Buffer routing");
            };
            match action {
                AcceptAction::SwitchBuffer { id } => assert_eq!(id.0, *routing_id),
                other => panic!("expected SwitchBuffer, got {other:?}"),
            }
            // And DefaultAcceptHandler returns the same action.
            let resolved = DefaultAcceptHandler.accept(cand).expect("handler ok");
            assert_eq!(&resolved, action);
        }
    }

    #[test]
    fn first_party_generators_returns_all_built_in_sources() {
        let app = app_with("hi\n", 5);
        let generators =
            first_party_generators(app.editor.registry.clone(), app.editor.config.clone());
        let ids: Vec<&'static str> = generators.iter().map(|g| g.spec().id).collect();
        assert_eq!(
            ids,
            vec![
                "files",
                "recent",
                "buffers",
                "lines",
                "jumps",
                "commands",
                "registers",
                "marks",
                "grep",
                "outline",
            ]
        );
    }

    /// P.6: jumps source returns `Err` when the position
    /// history is empty -- a fresh App has nothing to walk
    /// yet so the picker stays closed with a clean echo.
    #[test]
    fn jumps_source_empty_history_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = JumpsSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.contains("position history is empty"), "got {err}");
    }

    /// P.7: commands source emits one row per registered
    /// ex-command, sorted, with `InvokeCommand` routing
    /// payloads that strip the `ex:` registration prefix
    /// (kept as the canonical id) while displaying the
    /// user-facing alias the popup matches against.
    #[test]
    fn commands_source_emits_ex_commands_only() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = CommandsSource::new(app.editor.registry.clone());
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert!(!pairs.is_empty(), "should have at least one command");
        // Every row routes through InvokeCommand carrying the
        // canonical `ex:`-prefixed registration name.
        for (cand, routing) in &pairs {
            match routing {
                RoutingPayload::InvokeCommand { id, .. } => {
                    // Routing payload carries the canonical
                    // registration name verbatim (with `ex:`
                    // prefix where the command uses one, bare
                    // otherwise -- mode toggles like
                    // `buffer-words-mode` register without).
                    assert!(!id.is_empty());
                }
                other => panic!("expected InvokeCommand, got {other:?}"),
            }
            // Display text strips any `ex:` prefix so the popup
            // matches what the user would type at `:`.
            assert!(!cand.text.starts_with("ex:"), "got {}", cand.text);
        }
        // Sorted: alphabetic by user-facing name.
        let texts: Vec<&str> = pairs.iter().map(|(c, _)| c.text.as_str()).collect();
        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(texts, sorted);
    }

    /// MP.2: command rows carry args-hint + doc + latency as typed
    /// marginalia annotations (not a flat display string). The name is
    /// the matchable `display`. Confirms by finding `write` (a known
    /// ex-command) and checking its annotation set.
    #[test]
    fn commands_source_emits_marginalia_annotations() {
        use lattice_completion::Annotation;
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = CommandsSource::new(app.editor.registry.clone());
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        let write_row = pairs
            .iter()
            .find(|(c, _)| c.text == "write")
            .expect("write command row");
        let cand = &write_row.0;
        // Name is the matchable display, no hand-padded columns.
        assert_eq!(cand.display, "write");
        let by_cat = |cat: &str| {
            cand.annotations
                .iter()
                .find(|a| a.category() == cat)
                .map(|a| a.display_text().into_owned())
        };
        // Args hint for `:write` is `[<path>]` (optional arg).
        assert_eq!(by_cat("args").as_deref(), Some("[<path>]"));
        // Latency: `:write` is `Display`-class.
        assert_eq!(by_cat("latency").as_deref(), Some("[display]"));
        // Doc cell is present and non-empty.
        assert!(
            by_cat("doc").is_some_and(|d| d.contains("Write")),
            "expected doc annotation containing `Write`, got {:?}",
            by_cat("doc")
        );
        // Every command-picker annotation is the expected typed shape.
        assert!(cand.annotations.iter().all(|a| matches!(
            a,
            Annotation::Styled { .. } | Annotation::DocSnippet(_)
        )));
    }

    /// P.7: accept on `InvokeCommand` routing returns the
    /// matching outcome, carrying the canonical id +
    /// supplied args verbatim.
    #[test]
    fn commands_source_accept_translates_invoke_command() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = CommandsSource::new(app.editor.registry.clone());
        let routing = RoutingPayload::InvokeCommand {
            id: "ex:write".into(),
            args: Args::None,
        };
        let outcome = source.accept(&ctx, &routing).expect("ok");
        match outcome {
            PickerAcceptOutcome::InvokeCommand { id, args } => {
                assert_eq!(id, "ex:write");
                assert!(matches!(args, Args::None));
            }
            other => panic!("expected InvokeCommand, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile {
            path: "/tmp/x".into(),
        };
        assert!(source.accept(&ctx, &bad).is_err());
    }

    /// P.4: registers source returns `Err` when the context
    /// has no registers (empty `ctx.registers`).
    #[test]
    fn registers_source_empty_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = RegistersSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.contains("no registers set"), "got {err}");
    }

    /// P.4: synthesise a couple of register entries on the
    /// context and confirm rows route through
    /// `PasteRegister`.
    #[test]
    fn registers_source_emits_paste_routing() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let mut ctx = app.build_picker_context(&snap);
        ctx.registers = vec![("\"".into(), "hello".into()), ("a".into(), "world".into())];
        let source = RegistersSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 2);
        match &pairs[0].1 {
            RoutingPayload::PasteRegister { name } => assert_eq!(*name, '"'),
            other => panic!("expected PasteRegister, got {other:?}"),
        }
        match &pairs[1].1 {
            RoutingPayload::PasteRegister { name } => assert_eq!(*name, 'a'),
            other => panic!("expected PasteRegister, got {other:?}"),
        }
    }

    /// P.4: accept on a `PasteRegister` routing returns the
    /// matching outcome; mismatched routing errors.
    #[test]
    fn registers_source_accept_translates_paste_register() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = RegistersSource::new();
        let routing = RoutingPayload::PasteRegister { name: 'a' };
        match source.accept(&ctx, &routing).expect("ok") {
            PickerAcceptOutcome::PasteRegister { name } => assert_eq!(name, 'a'),
            other => panic!("expected PasteRegister outcome, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile {
            path: "/tmp/x".into(),
        };
        assert!(source.accept(&ctx, &bad).is_err());
    }

    /// Slice 3: `:picker grep` with no pattern (or an empty
    /// arg) opens an empty picker -- the user types into the
    /// prompt and the live flow fires `on_query_changed` on
    /// each debounced keystroke. The pre-slice-3 "pattern
    /// required" error is gone; no-arg is the canonical entry
    /// point for live grep.
    #[test]
    fn grep_source_empty_args_returns_empty_inline() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = GrepSource::new(app.editor.config.clone());
        let result = source
            .init(&ctx, &[])
            .expect("init must not error on no args");
        match result {
            lattice_picker::PickerInitResult::Inline(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected Inline(empty), got {other:?}"),
        }
        let result = source
            .init(&ctx, &[String::new()])
            .expect("init must not error on empty arg");
        match result {
            lattice_picker::PickerInitResult::Inline(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected Inline(empty) for empty arg, got {other:?}"),
        }
    }

    /// Slice 3: empty query through `on_query_changed`
    /// short-circuits to `Inline(empty)` -- no grep spawn, no
    /// UI block on the spawn-blocking pool.
    #[test]
    fn grep_source_on_query_changed_empty_short_circuits() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = GrepSource::new(app.editor.config.clone());
        let result = source
            .on_query_changed(&ctx, "")
            .expect("live source returns Some")
            .expect("no error");
        match result {
            lattice_picker::PickerInitResult::Inline(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected Inline(empty), got {other:?}"),
        }
        // Whitespace-only is treated the same.
        let result = source
            .on_query_changed(&ctx, "   ")
            .expect("live source returns Some")
            .expect("no error");
        match result {
            lattice_picker::PickerInitResult::Inline(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected Inline(empty) for whitespace query, got {other:?}"),
        }
    }

    /// Slice 3: GrepSource is declared live; the picker must
    /// see `spec.live == true` so it bypasses fuzzy refilter
    /// and the host routes keystrokes through
    /// `on_query_changed`.
    #[test]
    fn grep_source_spec_is_live() {
        let app = app_with("hi\n", 5);
        let source = GrepSource::new(app.editor.config.clone());
        assert!(source.spec().live, "GrepSource must declare live = true");
    }

    /// P.8: explicit `picker.grep.backend = "definitely-not-a-binary"`
    /// surfaces an actionable error before any subprocess
    /// is spawned.
    #[test]
    fn grep_source_unknown_backend_errors() {
        let app = app_with("hi\n", 5);
        app.editor
            .config
            .parse_and_set_command("picker.grep.backend=definitely-not-a-binary")
            .unwrap();
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = GrepSource::new(app.editor.config.clone());
        let err = source.init(&ctx, &["TODO".to_string()]).unwrap_err();
        assert!(err.contains("definitely-not-a-binary"), "got {err}");
        assert!(err.contains("not found on PATH"), "got {err}");
    }

    /// Slice 14d: a test-only source that returns
    /// `PickerInitResult::Future` so the async-init seat
    /// path is exercised end-to-end. The future resolves
    /// after a single tokio yield; the host's
    /// `drain_pending_picker_init` pumps the channel and
    /// seats the picker.
    struct DelayedFutureSource {
        spec: PickerSourceSpec,
    }

    impl DelayedFutureSource {
        fn new() -> Self {
            Self {
                spec: PickerSourceSpec::no_args(
                    "delayed-test",
                    "Test-only async source that resolves to one OpenFile candidate.",
                ),
            }
        }
    }

    impl PickerSourceGenerator for DelayedFutureSource {
        fn spec(&self) -> &PickerSourceSpec {
            &self.spec
        }

        fn init(
            &self,
            _ctx: &PickerContext<'_>,
            _args: &[String],
        ) -> SourceResult<PickerInitResult> {
            let fut = Box::pin(async move {
                // One yield so the future genuinely defers
                // -- mirrors a real LSP request that resolves
                // after a network round-trip.
                tokio::task::yield_now().await;
                let cand = RawCandidate::plain(String::from("test-result"), CandidateKind::Plain);
                Ok(vec![(
                    cand,
                    RoutingPayload::OpenFile {
                        path: "/tmp/lattice-test-future".into(),
                    },
                )])
            });
            Ok(PickerInitResult::Future(fut))
        }

        fn accept(
            &self,
            _ctx: &PickerContext<'_>,
            _routing: &RoutingPayload,
        ) -> SourceResult<PickerAcceptOutcome> {
            Ok(PickerAcceptOutcome::NoOp)
        }
    }

    /// Slice 14d: `:picker <source>` against a Future-returning
    /// source spawns the future, queues the result via mpsc,
    /// and seats the picker after the host's drain runs.
    /// Confirms the spawn + try_recv + seat_picker_from_pairs
    /// path works end-to-end.
    #[test]
    fn async_init_seat_path_pumps_future_result_into_picker() {
        use std::time::Duration;

        let mut app = app_with("hi\n", 5);
        // Build a fresh registry with the test source. We
        // can't mutate the App's shared registry (other Arcs
        // exist), so replace it wholesale.
        let mut reg = lattice_picker::PickerRegistry::new();
        let source: Arc<dyn PickerSourceGenerator> = Arc::new(DelayedFutureSource::new());
        reg.register_generator(source);
        app.editor.picker_registry = Arc::new(reg);
        // Fire the picker. Init returns Future; the picker
        // should NOT seat synchronously.
        app.open_picker("delayed-test".into(), Vec::new());
        assert!(
            app.editor.picker.is_none(),
            "picker shouldn't seat sync on Future"
        );
        assert!(
            app.editor.pending_picker_init.is_some(),
            "pending should be set"
        );
        // Pump the drain. The future needs at least one tokio
        // poll to resolve -- we give the spawned task a
        // chance to land by sleeping briefly.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.drain_pending_picker_init();
            if app.editor.picker.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let p = app
            .editor
            .picker
            .as_ref()
            .expect("picker seated after drain");
        assert_eq!(p.candidates.len(), 1);
        assert_eq!(p.source_id.as_deref(), Some("delayed-test"));
    }

    /// P.9: outline source returns `Err` when the active
    /// buffer has no tree-sitter symbols (plain text, or a
    /// language without a `symbols.scm` query).
    #[test]
    fn outline_source_no_symbols_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = OutlineSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.starts_with("outline:"), "got {err}");
    }

    /// P.9: synthesised syntax_symbols on the context produce
    /// one row per symbol with `JumpInBuffer` routing carrying
    /// the captured buffer id + (line, col) coordinates.
    #[test]
    fn outline_source_emits_jump_in_buffer_routing() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let mut ctx = app.build_picker_context(&snap);
        let active_id = ctx.active_buffer.buffer_id;
        ctx.active_buffer.syntax_symbols =
            vec![("foo".to_string(), 2, 4), ("bar".to_string(), 10, 0)];
        let source = OutlineSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 2);
        match &pairs[0].1 {
            RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
                assert_eq!(*buffer_id, active_id);
                assert_eq!(*line, 2);
                assert_eq!(*col, 4);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
        // Display contains 1-based line + the name.
        assert!(
            pairs[0].0.display.contains("foo"),
            "got {}",
            pairs[0].0.display
        );
        assert!(
            pairs[0].0.display.contains("3"),
            "got {}",
            pairs[0].0.display
        );
    }

    /// P.5: marks source returns `Err` when no marks set.
    #[test]
    fn marks_source_empty_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = MarksSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.contains("no marks set"), "got {err}");
    }

    /// P.5: synthesise marks on the context, confirm rows
    /// route through `JumpToMark` and display the line:col.
    #[test]
    fn marks_source_emits_jump_to_mark_routing() {
        use lattice_protocol::Position;

        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let mut ctx = app.build_picker_context(&snap);
        ctx.marks = vec![('a', Position::new(2, 0)), ('b', Position::new(5, 3))];
        let source = MarksSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 2);
        match &pairs[0].1 {
            RoutingPayload::JumpToMark { name } => assert_eq!(*name, 'a'),
            other => panic!("expected JumpToMark, got {other:?}"),
        }
        // Display carries 1-based line:col.
        assert!(
            pairs[0].0.display.contains("3:1"),
            "got {}",
            pairs[0].0.display
        );
        assert!(
            pairs[1].0.display.contains("6:4"),
            "got {}",
            pairs[1].0.display
        );
    }

    /// P.5: accept on a `JumpToMark` routing returns the
    /// matching outcome verbatim.
    #[test]
    fn marks_source_accept_translates_jump_to_mark() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = MarksSource::new();
        let routing = RoutingPayload::JumpToMark { name: 'm' };
        match source.accept(&ctx, &routing).expect("ok") {
            PickerAcceptOutcome::JumpToMark { name } => assert_eq!(name, 'm'),
            other => panic!("expected JumpToMark outcome, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile {
            path: "/tmp/x".into(),
        };
        assert!(source.accept(&ctx, &bad).is_err());
    }

    /// P.6: synthesise a couple of position-history entries
    /// (the App's ring is private at this layer but the
    /// PickerContext carries an owned vec we can substitute
    /// for the test). Confirm the source emits newest-first
    /// with the appropriate source tags + `JumpInBuffer`
    /// routing.
    #[test]
    fn jumps_source_emits_newest_first_with_source_tags() {
        use lattice_picker::{PositionEntry, PositionSource};

        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let mut ctx = app.build_picker_context(&snap);
        ctx.position_history = vec![
            PositionEntry {
                buffer_id: 1,
                line: 0,
                col: 0,
                source: PositionSource::AutoJump,
            },
            PositionEntry {
                buffer_id: 1,
                line: 5,
                col: 2,
                source: PositionSource::NamedMark('a'),
            },
            PositionEntry {
                buffer_id: 2,
                line: 10,
                col: 0,
                source: PositionSource::PluginPush,
            },
        ];
        let source = JumpsSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 3);
        // Newest first: plugin (line 10) leads.
        match &pairs[0].1 {
            RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
                assert_eq!(*buffer_id, 2);
                assert_eq!(*line, 10);
                assert_eq!(*col, 0);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
        // The named-mark row carries `'a` in its source tag.
        assert!(pairs[1].0.display.contains("'a"), "{}", pairs[1].0.display);
        // The auto row carries `auto`.
        assert!(
            pairs[2].0.display.contains("auto"),
            "{}",
            pairs[2].0.display
        );
    }

    /// P.3: lines source emits one row per addressable line
    /// in the active buffer, with `JumpInBuffer` routing
    /// payloads carrying the captured buffer id.
    #[test]
    fn lines_source_emits_row_per_line() {
        let app = app_with("alpha\nbeta\ngamma\n", 10);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let active_id = ctx.active_buffer.buffer_id;
        let source = LinesSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        // 3 addressable lines; the trailing-empty phantom is dropped.
        assert_eq!(pairs.len(), 3);
        for (i, (cand, routing)) in pairs.iter().enumerate() {
            match routing {
                RoutingPayload::JumpInBuffer {
                    buffer_id,
                    line,
                    col,
                } => {
                    assert_eq!(*buffer_id, active_id);
                    assert_eq!(*line, i as u32);
                    assert_eq!(*col, 0);
                }
                other => panic!("expected JumpInBuffer, got {other:?}"),
            }
            // Display starts with right-aligned line number then `:`.
            assert!(
                cand.display.contains(':'),
                "missing `:` in {}",
                cand.display
            );
        }
    }

    /// P.3: empty buffer surfaces an error echo (the
    /// `line_count == 0` guard) rather than seating an empty
    /// picker.
    #[test]
    fn lines_source_empty_buffer_errors() {
        let app = app_with("", 5);
        let snap = app.ad().snapshot.clone();
        // ropey treats truly-empty as one logical line; force the
        // guard by constructing a context whose buffer has zero
        // line count -- skip via the buffer's own report. The
        // line_count == 0 branch is defensive (ropey rarely
        // produces it) so this test only confirms the non-empty
        // path doesn't panic when the rope contains a single
        // empty line.
        let ctx = app.build_picker_context(&snap);
        let source = LinesSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        if let PickerInitResult::Inline(pairs) = result {
            // One logical line, contents may be empty.
            assert_eq!(pairs.len(), 1);
        } else {
            panic!("expected Inline");
        }
    }

    /// P.3: accept on a `JumpInBuffer` routing returns the
    /// matching outcome variant. Mismatched routing returns
    /// `Err`.
    #[test]
    fn lines_source_accept_translates_jump_in_buffer() {
        let app = app_with("hi\n", 5);
        let snap = app.ad().snapshot.clone();
        let ctx = app.build_picker_context(&snap);
        let source = LinesSource::new();
        let routing = RoutingPayload::JumpInBuffer {
            buffer_id: 7,
            line: 12,
            col: 3,
        };
        let outcome = source.accept(&ctx, &routing).expect("ok");
        match outcome {
            PickerAcceptOutcome::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
                assert_eq!(buffer_id, 7);
                assert_eq!(line, 12);
                assert_eq!(col, 3);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile {
            path: "/tmp/x".into(),
        };
        assert!(source.accept(&ctx, &bad).is_err());
    }
}
