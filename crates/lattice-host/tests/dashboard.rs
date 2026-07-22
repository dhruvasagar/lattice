//! DB.2 — `*dashboard*`-is-a-buffer integration tests.
//!
//! Drives a real `Editor::boot` (which runs the Phase-B install list, so
//! `dashboard-mode` + the `DashboardRegistry` service + `:dashboard` are all
//! registered) and exercises `do_open_dashboard` end-to-end: the buffer is
//! created, named, activated, read-only via `dashboard-mode`, carries the
//! `HelpLinks` local so `<CR>`-follow works, and re-opening is idempotent.
//!
//! Design: `docs/dev/architecture/dashboard.md` §9.

use std::collections::HashSet;

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_dashboard::DashboardMode;
use lattice_host::editor::Editor;
use lattice_host::modes::HelpLinks;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

/// Build the render state, run the cells worker, and return the set of
/// distinct foreground colours across all non-blank rendered cells.
fn rendered_fg_colors(editor: &mut Editor) -> HashSet<u32> {
    let rs = editor.build_render_state();
    editor.render_state.store(std::sync::Arc::new(rs));
    lattice_host::cells_worker::recompute(&editor.render_state);
    let cells = editor.render_state.load().cells.load();
    let mut fgs = HashSet::new();
    for pane in cells.panes.iter() {
        let matrix = pane.matrix.load();
        for chunk in matrix.chunks.iter() {
            for row in chunk.rows.iter() {
                for cell in row.cells.iter() {
                    if cell.codepoint != 0x20 && cell.codepoint != 0 {
                        fgs.insert(cell.fg);
                    }
                }
            }
        }
    }
    fgs
}

#[test]
fn dashboard_opens_named_and_active() {
    let mut editor = boot();
    editor.do_open_dashboard();

    let id = editor
        .buffers
        .by_name("*dashboard*")
        .expect("*dashboard* buffer should exist after :dashboard");

    // Distinct kind, and it is the active buffer.
    assert_eq!(editor.buffers.kind_of(id), Some(BufferKind::Dashboard));
    assert_eq!(editor.active_buffer, BufferKind::Dashboard);
}

#[test]
fn dashboard_major_is_dashboard_mode() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // dashboard-mode is the major, which is what contributes ReadOnly /
    // NoFile / gutterless.
    let major = editor.active_modes.get(&id).and_then(|m| m.major());
    assert_eq!(major, Some(DashboardMode::mode_id()));
}

#[test]
fn dashboard_content_has_section_pointers() {
    let mut editor = boot();
    editor.do_open_dashboard();

    // The dashboard is active, so active_text() is its body content. The brand
    // wordmark lives in the DB.4 virtual-row banner, not the body.
    let text = editor.active_text().as_string();
    assert!(text.contains("tutor"), "tutor pointer missing");
    assert!(text.contains("help"), "help pointer missing");
}

/// Every `:help <topic>` link the dashboard renders must resolve to a
/// registered help topic — a `LinkTarget::Topic` with no backing doc is a
/// dead `<CR>`. This is the seam where the dashboard's section registry and
/// the help-topic registry (both linked by the host) actually meet, so it's
/// the right place to pin cross-consistency. Guards the `getting-started`
/// link the launch page leads with, and the earlier `commands`/`config`
/// dead links (the real docs are `ex-commands`/`options`).
#[test]
fn dashboard_help_topic_links_resolve_to_registered_topics() {
    use lattice_dashboard::{DashboardCtx, LinkTarget, SectionSelection};

    let topics = lattice_help::topics::builtin_topics();
    let registry = lattice_dashboard::builtin_registry();
    let ctx = DashboardCtx::default();

    let mut checked = 0;
    for section in registry.ordered(&SectionSelection::Default) {
        for row in section.render(&ctx).rows {
            for span in row.spans {
                if let Some(LinkTarget::Topic(name)) = span.link {
                    assert!(
                        topics.lookup(&name).is_some(),
                        "dashboard links `:help {name}` but no such help topic \
                         is registered (topic name must match a docs/user/*.md \
                         file stem)"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked > 0,
        "expected the dashboard to render some help-topic links"
    );
}

// Following a dashboard link end-to-end: `<CR>` (Action::FollowLink) on a
// link must resolve to the correct behaviour. Covers the follow-guard
// (Dashboard grouped with Help) + the link scheme (`exec:` runs a command,
// `help:` opens a topic) — the three bugs that made every dashboard link a
// silent no-op. The input-gate half (`<CR>` → Action::FollowLink for the
// Dashboard buffer) is pinned in `lattice-ui-tui/src/input.rs`
// (`dashboard_active_routes_enter_to_follow_link`).
mod link_following {
    use lattice_grammar::Effect;
    use lattice_help::HelpLinkTarget;
    use lattice_host::action::Action;
    use lattice_host::dispatch::RendererSignal;
    use lattice_host::modes::HelpLinks;

    use super::*;

    /// Open the dashboard, then move the cursor onto the first link whose
    /// target satisfies `want`, and return that link's target. Panics if no
    /// such link is seeded — a regression in the scheme mapping.
    fn cursor_on_link(
        editor: &mut Editor,
        want: impl Fn(&HelpLinkTarget) -> bool,
    ) -> HelpLinkTarget {
        editor.do_open_dashboard();
        let id = editor.buffers.by_name("*dashboard*").unwrap();
        let link = editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<HelpLinks>())
            .and_then(|hl| hl.0.iter().find(|l| want(&l.target)).cloned())
            .expect("a dashboard link matching the predicate should be seeded");
        editor.cursor = link.range.start;
        link.target
    }

    #[test]
    fn following_the_tutor_link_starts_the_tutor() {
        let mut editor = boot();
        // The tutor pointer is `LinkTarget::Command("tutor")` → seeded as
        // `Execute("tutor")` (runs `:tutor`), NOT `Command` (which would only
        // *describe* the command).
        let target = cursor_on_link(
            &mut editor,
            |t| matches!(t, HelpLinkTarget::Execute(c) if c == "tutor"),
        );
        assert_eq!(target, HelpLinkTarget::Execute("tutor".to_string()));

        // Following the link runs `:tutor`, which resolves to
        // `Effect::Tutor` — the effect that starts the tutor. (The effect's
        // application, `do_tutor`, is renderer-coupled — it opens the lesson
        // file — so it lands in `App::apply_effect`, not the host's
        // `handle_effect`; the host-observable proof is that the correct
        // effect is emitted, i.e. the scheme + follow-guard resolved.)
        let outcome = editor.dispatch(Action::FollowLink);
        assert!(
            outcome
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Tutor { lesson: None })),
            "following the tutor link should emit Effect::Tutor (start the \
             tutor); got effects={:?}",
            outcome.effects
        );
    }

    #[test]
    fn following_a_help_topic_link_opens_that_help_page() {
        let mut editor = boot();
        // A `:help <topic>` pointer is `LinkTarget::Topic(name)` → seeded as
        // `Topic(name)` (opens the help page), NOT `Unresolved`.
        let target = cursor_on_link(&mut editor, |t| matches!(t, HelpLinkTarget::Topic(_)));
        let HelpLinkTarget::Topic(topic_name) = target.clone() else {
            unreachable!()
        };

        let outcome = editor.dispatch(Action::FollowLink);

        // Opening a help topic emits a `DisplayBuffer` signal categorised as
        // a help topic — the renderer turns that into the visible help page.
        let opened_help_topic = outcome.renderer_signals.iter().any(|s| {
            matches!(
                s,
                RendererSignal::DisplayBuffer(req)
                    if matches!(
                        req.category,
                        lattice_core::ui::display::BufferDisplayCategory::HelpTopic
                    )
            )
        });
        assert!(
            opened_help_topic,
            "following the `:help {topic_name}` link should open that help page \
             (expected a DisplayBuffer/HelpTopic signal, got {:?})",
            outcome.renderer_signals
        );
    }

    #[test]
    fn dashboard_project_links_seed_as_external_urls() {
        // The dashboard's project links (`GitHub`, `Issues`) are
        // `LinkTarget::Url(https://…)` and must seed as `HelpLinkTarget::Url`
        // so the follow handler routes them to the OS handler
        // (`open` / `xdg-open`). We pin the CLASSIFICATION, not the follow:
        // following would call `open_external_uri`, whose spawn the codebase
        // convention deliberately leaves untested (it pops a real browser —
        // see `tests/show_document_open_effect.rs`). The `Url` arm itself is
        // a one-line inline `self.open_external_uri(&url)`, identical to the
        // documentLink follow.
        let mut editor = boot();
        editor.do_open_dashboard();
        let id = editor.buffers.by_name("*dashboard*").unwrap();
        let urls: Vec<String> = editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<HelpLinks>())
            .map(|hl| {
                hl.0.iter()
                    .filter_map(|l| match &l.target {
                        HelpLinkTarget::Url(u) => Some(u.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            urls.iter()
                .any(|u| u == "https://github.com/dhruvasagar/lattice"),
            "the GitHub link should seed as HelpLinkTarget::Url; got {urls:?}"
        );
        assert!(
            urls.iter()
                .any(|u| u == "https://github.com/dhruvasagar/lattice/issues"),
            "the Issues link should seed as HelpLinkTarget::Url; got {urls:?}"
        );
    }
}

#[test]
fn dashboard_registers_branding_virtual_rows() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // DB.4: the branding provider is registered for the dashboard buffer and
    // emits the mark + wordmark rows, colored with the brand blue.
    let providers = editor.virtual_row_providers.snapshot(id);
    assert!(
        !providers.is_empty(),
        "branding provider should be registered"
    );
    let brand_blue = lattice_theme::Color::Rgb(0x1f, 0x6f, 0xeb).to_rgb_u32(0);
    let has_blue_glyph = providers.iter().flat_map(|p| p.collect()).any(|row| {
        row.cells
            .iter()
            .any(|c| c.fg == brand_blue && c.codepoint != 0x20)
    });
    let has_wordmark = providers.iter().flat_map(|p| p.collect()).any(|row| {
        let t: String = row
            .cells
            .iter()
            .filter_map(|c| char::from_u32(c.codepoint))
            .collect();
        t.contains("Lattice")
    });
    assert!(
        has_blue_glyph,
        "branding should render brand-blue mark glyphs"
    );
    assert!(has_wordmark, "branding should render the Lattice wordmark");
}

#[test]
fn dashboard_seeds_help_links_for_follow() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    let links = editor
        .buffer_locals
        .get(&id)
        .and_then(|locals| locals.get::<HelpLinks>())
        .map(|hl| hl.0.len())
        .unwrap_or(0);
    assert!(
        links > 0,
        "dashboard should seed HelpLinks so <CR>-follow works (gated on \
         help-mode-independent path via the Dashboard kind)"
    );
}

#[test]
fn dashboard_activates_help_mode_and_is_gutterless() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let id = editor.buffers.by_name("*dashboard*").unwrap();

    // help-mode is a companion minor (its good defaults: read-only, wrap,
    // no-file, gutterless).
    let has_help = editor
        .active_modes
        .get(&id)
        .map(|m| m.has_minor(lattice_mode::HelpMode::mode_id()))
        .unwrap_or(false);
    assert!(has_help, "help-mode should be active on the dashboard");
    assert!(!editor.option_cache.show_line_numbers, "no line numbers");
    assert!(!editor.option_cache.sign_column, "no signcolumn");
}

#[test]
fn dashboard_registers_theme_elements() {
    let mut editor = boot();
    editor.do_open_dashboard();

    // dashboard-mode's on_activate registers the dashboard.* elements against
    // the host's ThemeRegistry service (DB.3).
    let theme = editor
        .services
        .get::<lattice_theme::ThemeRegistryHandle>()
        .expect("theme registry service registered at boot");
    for name in ["dashboard.logo", "dashboard.cursor", "dashboard.title"] {
        assert!(
            theme
                .id(&lattice_theme::ElementName::from(name.to_string()))
                .is_some(),
            "{name} should be registered after :dashboard"
        );
    }
}

#[test]
fn dashboard_renders_through_colored_matrix_not_raw_fallback() {
    // Regression: the pane-cells builder allowlist must include
    // BufferKind::Dashboard, else the dashboard pane is skipped, no
    // DisplayMatrix is built, and the TUI falls back to uncoloured raw text
    // (the "no colors on the dashboard" bug).
    let mut editor = boot();
    editor.viewport_height = 40;
    editor.do_open_dashboard();

    let fgs = rendered_fg_colors(&mut editor);
    assert!(
        fgs.len() >= 2,
        "dashboard should render multiple distinct fg colors (markdown \
         headings + links + body), got {} — the DisplayMatrix was likely \
         skipped and the pane fell back to raw text",
        fgs.len()
    );
}

#[test]
fn dashboard_branding_reaches_pane_virtual_row_matrix() {
    // Regression: registering the provider is not enough — the branding rows
    // must reach the pane's VirtualRowMatrix (the "registered but not
    // rendered" failure mode). Drive the virtual-rows worker and assert the
    // dashboard pane's matrix carries the banner rows.
    let mut editor = boot();
    editor.viewport_height = 40;
    editor.do_open_dashboard();

    let rs = editor.build_render_state();
    editor.render_state.store(std::sync::Arc::new(rs));
    let mut state = lattice_host::virtual_rows_worker::VirtualRowsWorkerState::default();
    lattice_host::virtual_rows_worker::recompute(
        &mut state,
        &editor.render_state,
        &editor.virtual_row_providers,
    );

    let cells = editor.render_state.load().cells.load();
    let total_rows: usize = cells
        .panes
        .iter()
        .map(|p| p.virtual_rows_matrix.load().rows.len())
        .sum();
    assert!(
        total_rows >= lattice_dashboard::BRANDING_ROW_COUNT,
        "branding rows should reach the pane virtual-row matrix, got {total_rows}"
    );
}

#[test]
fn dashboard_centering_widens_the_gutter_not_the_text() {
    let mut editor = boot();
    editor.pane_tree.active_mut().viewport_width = 120;
    editor.do_open_dashboard();

    // Centring is gutter-based: content_left_pad > 0, and the buffer TEXT is
    // NOT mutated (markdown headers stay at column 0 so their styling survives).
    assert!(
        editor.option_cache.content_left_pad > 0,
        "content_left_pad should be set for centring at width 120"
    );
    let text = editor.active_text().as_string();
    assert!(
        text.lines().any(|l| l.starts_with('#')),
        "markdown headers must remain at column 0 (no text padding)"
    );
}

/// DB.6 test-bullet pin ("resize re-centres"): a resize AFTER the dashboard
/// is already open must re-derive `content_left_pad` from the new width —
/// WITHOUT a recompose. `content_left_pad` is computed fresh from the live
/// viewport width on every `rebuild_option_cache` call (not baked in at
/// compose time), and `Editor::set_pane_viewport`'s actor handler already
/// calls `rebuild_option_cache` on every active-pane resize (DB.4) — this
/// pins that behavior directly against the `Editor` method a resize
/// ultimately drives, without needing the full actor.
#[test]
fn dashboard_resize_after_open_recentres_without_recompose() {
    let mut editor = boot();
    editor.pane_tree.active_mut().viewport_width = 120;
    editor.do_open_dashboard();
    let narrow_pad = editor.option_cache.content_left_pad;

    editor.pane_tree.active_mut().viewport_width = 200;
    editor.rebuild_option_cache();
    let wide_pad = editor.option_cache.content_left_pad;

    assert!(
        wide_pad > narrow_pad,
        "widening the pane should increase content_left_pad \
         (narrow={narrow_pad}, wide={wide_pad})"
    );
}

#[test]
fn dashboard_reopen_is_idempotent() {
    let mut editor = boot();
    editor.do_open_dashboard();
    let first = editor.buffers.by_name("*dashboard*").unwrap();

    // Switch away, then re-open: same buffer id, no duplicate.
    editor.do_open_dashboard();
    let second = editor.buffers.by_name("*dashboard*").unwrap();
    assert_eq!(first, second, "re-opening must not create a second buffer");
}

// DB.5 — startup gating + mode-owned trigger.
//
// `lattice_dashboard::install`'s startup-trigger subscription (design.md
// §9.1) spawns its wait-for-`Startup` task on the shared LSP runtime (the
// same process-wide runtime `Editor::boot` hands every subsystem via
// `boot.runtime_handle()`), so these tests need their own `#[tokio::test]`
// runtime to `.await` the cross-runtime `async_landed` wake — same pattern
// `lsp_async_wake.rs` (AW.1) uses.
mod startup_gating {
    use std::path::PathBuf;
    use std::time::Duration;

    use lattice_core::DocumentBuilder;
    use lattice_dashboard::DashboardEnabled;

    use super::*;

    /// Simulate a renderer's post-boot seam: capture `opened_file` from the
    /// `Document` BEFORE it moves into `Editor::boot` (mirrors
    /// `lattice-ui-tui`'s `App::new` / `lattice-ui-gpui`'s `GpuiApp::new`),
    /// boot, then publish `Startup` — exactly what DB.5 wires at both
    /// renderer seams.
    fn boot_with_startup(document: CoreDocument) -> Editor {
        let opened_file = document.path().map(|p| p.to_path_buf());
        let editor = Editor::boot(document);
        editor
            .event_bus
            .publish_typed(lattice_mode::Startup { opened_file });
        editor
    }

    /// Wait (bounded) for the startup-trigger task to wake the editor, then
    /// drain the tick so a pending `Effect::OpenDashboard` (if any) applies.
    /// A bounded timeout, not an unconditional wait, because the
    /// dashboard-disabled / file-arg cases never send anything — those
    /// paths only need the timeout to elapse without a false failure.
    async fn settle_startup_trigger(editor: &mut Editor) {
        let _ = tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified()).await;
        editor.run_tick_pending();
    }

    #[tokio::test]
    async fn no_file_and_enabled_auto_opens_dashboard() {
        let mut editor = boot_with_startup(CoreDocument::from_text("scratch\n"));
        settle_startup_trigger(&mut editor).await;

        assert_eq!(
            editor.active_buffer,
            BufferKind::Dashboard,
            "no file + dashboard.enabled (default true) should auto-open *dashboard*"
        );
    }

    #[tokio::test]
    async fn file_arg_leaves_file_active_but_dashboard_stays_reachable() {
        let doc = DocumentBuilder::default()
            .with_text("fn main() {}\n")
            .with_path(PathBuf::from("/tmp/db5_startup_gating_test.rs"))
            .build();
        let mut editor = boot_with_startup(doc);
        settle_startup_trigger(&mut editor).await;

        assert_ne!(
            editor.active_buffer,
            BufferKind::Dashboard,
            "opening with a file argument must not auto-show the dashboard"
        );
        // Reachable on demand — the applier is the same one `:dashboard` uses.
        editor.do_open_dashboard();
        assert_eq!(editor.active_buffer, BufferKind::Dashboard);
    }

    #[tokio::test]
    async fn disabled_skips_auto_open_but_command_still_works() {
        let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
        editor
            .config
            .set_typed::<DashboardEnabled>(false)
            .expect("dashboard.enabled should accept a bool");
        editor
            .event_bus
            .publish_typed(lattice_mode::Startup { opened_file: None });
        settle_startup_trigger(&mut editor).await;

        assert_ne!(
            editor.active_buffer,
            BufferKind::Dashboard,
            "dashboard.enabled=false must not auto-open *dashboard*"
        );
        editor.do_open_dashboard();
        assert_eq!(
            editor.active_buffer,
            BufferKind::Dashboard,
            ":dashboard should still work on demand when auto-open is disabled"
        );
    }

    /// Regression pin for the DB.5 `ConfigRegistry` hoist
    /// (`editor_boot.rs`): `Arc<ConfigRegistry>` must land as a Phase-A
    /// service (registered before the Phase-B install list runs), or
    /// `lattice_dashboard::install`'s `boot.service::<Arc<ConfigRegistry>>()`
    /// call — made synchronously, during Phase-B — permanently observes
    /// `None` (a `ServiceRegistry` lookup from inside a Phase-B installer
    /// can never see a registration added later in the same boot call).
    /// Pinning service-availability post-boot guards against a future
    /// refactor silently moving the registration back down.
    #[test]
    fn config_registry_is_resolvable_as_a_service_after_boot() {
        let editor = boot();
        assert!(
            editor
                .services
                .get::<std::sync::Arc<lattice_config::ConfigRegistry>>()
                .is_some(),
            "Arc<ConfigRegistry> should be a resolvable service after boot — \
             lattice_dashboard::install (and any other Phase-B installer) \
             reads it synchronously during install, so it must be a Phase-A \
             registration"
        );
    }
}

// DB.6 — `dashboard.source` full override + recompose triggers.
//
// The recompose-trigger tests need their own `#[tokio::test]` runtime for
// the same cross-runtime-wake reason `startup_gating` does: the
// subscription's drain task runs on the shared LSP runtime
// (`boot.runtime_handle()`), and `send` on its matching `InboundBus` wakes
// `editor.async_landed`, which only a live async context can `.await`.
mod source_override_and_recompose {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use lattice_dashboard::{DashboardSections, DashboardSource};

    use super::*;

    /// A scratch file for `dashboard.source` tests. Removed on drop so
    /// parallel test runs never see each other's fixtures and nothing
    /// leaks into the real temp dir.
    struct TempFile(PathBuf);

    impl TempFile {
        fn write(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir().join(name);
            fs::write(&path, contents).expect("write temp dashboard.source fixture");
            Self(path)
        }

        fn path_string(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn dashboard_source_replaces_section_composition() {
        let file = TempFile::write(
            "db6_source_override_test.md",
            "# Custom Page\n\nHand-authored content.\n",
        );
        let mut editor = boot();
        editor
            .config
            .set_typed::<DashboardSource>(file.path_string())
            .expect("dashboard.source should accept a path string");
        editor.do_open_dashboard();

        let text = editor.active_text().as_string();
        assert!(
            text.contains("Custom Page"),
            "override file content should appear verbatim, got: {text:?}"
        );
        assert!(
            !text.contains("tutor"),
            "an active dashboard.source override should fully REPLACE section \
             composition, not merge with it — got: {text:?}"
        );
    }

    #[test]
    fn dashboard_source_missing_falls_back_to_sections() {
        let mut editor = boot();
        editor
            .config
            .set_typed::<DashboardSource>("/nonexistent/db6_missing_source.md".to_string())
            .expect("dashboard.source should accept a path string");
        editor.do_open_dashboard();

        let text = editor.active_text().as_string();
        assert!(
            text.contains("tutor"),
            "a missing dashboard.source must fall back to section composition, \
             never an empty page — got: {text:?}"
        );
    }

    /// Wait (bounded) for the recompose-trigger task to wake the editor,
    /// then drain the tick so a pending `Effect::OpenDashboard` (if any)
    /// applies. Mirrors `startup_gating::settle_startup_trigger`.
    async fn settle_recompose_trigger(editor: &mut Editor) {
        let _ = tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified()).await;
        editor.run_tick_pending();
    }

    #[tokio::test]
    async fn dashboard_sections_change_recomposes_an_open_dashboard() {
        let mut editor = boot();
        editor.do_open_dashboard();
        assert!(
            editor.active_text().as_string().contains("tutor"),
            "sanity: the default selection includes the tutor pointer"
        );

        editor
            .config
            .set_typed::<DashboardSections>("about".to_string())
            .expect("dashboard.sections should accept a string");
        settle_recompose_trigger(&mut editor).await;

        let text = editor.active_text().as_string();
        assert!(
            !text.contains("tutor"),
            "dashboard.sections=about should recompose the OPEN dashboard down to \
             just the about section, got: {text:?}"
        );
    }

    #[tokio::test]
    async fn dashboard_sections_change_does_not_auto_open_a_closed_dashboard() {
        let mut editor = boot();
        // Dashboard never opened — active buffer is still the scratch doc.
        editor
            .config
            .set_typed::<DashboardSections>("about".to_string())
            .expect("dashboard.sections should accept a string");
        settle_recompose_trigger(&mut editor).await;

        assert_ne!(
            editor.active_buffer,
            BufferKind::Dashboard,
            "a dashboard.sections edit must not auto-open the dashboard when it \
             isn't already open — only :dashboard / startup do that"
        );
    }
}

// DB.7 — the idle-frame correctness half of design.md §13's two assertions.
// `benches/dashboard.rs`'s `dashboard_idle_tick` bench is the numeric half
// (idle ticks are fast); this is the "enforced, not asserted" pin that
// they're not just fast but do literally ZERO recompose work — the
// document's version must not advance across idle ticks with nothing
// published, the same class of guarantee the H.3 bug story
// (BENCHMARKS.md) argues a bench alone can't prove.
mod idle_frame_does_no_recompose {
    use super::*;

    fn document_version(editor: &Editor, id: lattice_core::BufferId) -> u64 {
        editor
            .buffers
            .with_document(id, |doc| doc.handle.snapshot().version)
            .expect("dashboard document should exist")
    }

    #[test]
    fn dashboard_idle_ticks_do_not_recompose() {
        let mut editor = boot();
        editor.do_open_dashboard();
        let id = editor.buffers.by_name("*dashboard*").unwrap();
        let before = document_version(&editor, id);

        // No effect, no config change, no event published — just idle ticks,
        // the same drain a renderer runs every frame.
        for _ in 0..20 {
            editor.run_tick_pending();
        }

        let after = document_version(&editor, id);
        assert_eq!(
            before, after,
            "idle ticks with nothing published must not recompose the \
             dashboard document — its version should not advance (guards \
             paramount #1: idle frames do zero dashboard work)"
        );
    }
}

#[cfg(test)]
mod major_gating {
    use super::*;
    use lattice_protocol::position::Position;
    use lattice_protocol::{KeyChord, parse_chord_sequence};

    fn chord(s: &str) -> KeyChord {
        parse_chord_sequence(s)
            .expect("parse")
            .into_iter()
            .next()
            .expect("one")
    }

    /// Regression: the ai-conversation major mode binds all of
    /// `i a o A I O` to focus-prompt, which jumps the cursor to the END OF
    /// CONTENT (the last line) and enters Insert. Because major-mode
    /// keymaps were folded into the always-on merge, those bindings fired
    /// in EVERY buffer — pressing one on the dashboard jumped to the last
    /// line and dragged the viewport to the bottom. With major layers
    /// gated by the active major, none of them may move the cursor off
    /// its line (a builtin `A`/`I` still moves within line 0, which is
    /// fine — it is not the focus-prompt EOF jump).
    #[test]
    fn focus_prompt_chords_do_not_leak_onto_dashboard() {
        for key in ["i", "a", "o", "A", "I", "O"] {
            let mut editor = boot();
            editor.viewport_height = 20;
            editor.do_open_dashboard();
            assert_eq!(editor.cursor, Position::ZERO, "dashboard opens at top");
            let last_line = editor.active_text().line_count().saturating_sub(1);
            assert!(last_line > 1, "dashboard has multi-line content");
            let mut partial: Vec<KeyChord> = Vec::new();
            editor.dispatch_chord(chord(key), &mut partial);
            assert_eq!(
                editor.cursor.line, 0,
                "`{key}` must NOT jump the cursor to the end of content on the \
                 dashboard (ai-conversation focus-prompt leaked via an ungated \
                 MajorMode layer); landed on line {}",
                editor.cursor.line
            );
        }
    }
}

#[cfg(test)]
mod insert_still_works {
    use super::*;
    use lattice_protocol::position::Position;
    use lattice_protocol::{KeyChord, parse_chord_sequence};

    fn chord(s: &str) -> KeyChord {
        parse_chord_sequence(s)
            .expect("parse")
            .into_iter()
            .next()
            .expect("one")
    }

    /// Non-regression: `i` on a normal editable buffer still enters Insert
    /// AT THE CURSOR (no jump to EOF). The major-gating fix must not have
    /// disabled the builtin `i`.
    #[test]
    fn i_on_editable_buffer_enters_insert_at_cursor() {
        let mut editor = boot(); // scratch "scratch\n", editable
        editor.viewport_height = 20;
        editor.cursor = Position::new(0, 3);
        let mut partial: Vec<KeyChord> = Vec::new();
        editor.dispatch_chord(chord("i"), &mut partial);
        assert!(
            matches!(editor.modal, lattice_grammar::ModalState::Insert),
            "i must still enter Insert on a normal buffer"
        );
        assert_eq!(
            editor.cursor,
            Position::new(0, 3),
            "i must enter Insert AT THE CURSOR, not jump to EOF"
        );
    }
}

#[cfg(test)]
mod owc_adopt {
    use super::*;

    /// The end-to-end guarantee: after opening the dashboard, the FIRST
    /// keystroke (which runs `maybe_adopt_owner_write` before dispatch) must
    /// NOT move the cursor to EOF. The dashboard has no editable tail, so the
    /// OWC adopt is gated off — the forced top-of-page caret stands even though
    /// the owner-write populate left the document selection at EOF.
    #[test]
    fn keystroke_after_dashboard_open_does_not_jump_to_eof() {
        use lattice_host::action::Action;
        let mut editor = boot();
        editor.viewport_height = 20;
        editor.do_open_dashboard();
        // `dispatch_fused` is the real per-keystroke entry; it runs the OWC
        // adopt. `Action::None` is an inert keystroke.
        let pre = editor.active_buffer;
        let _ = editor.dispatch_fused(Action::None, pre, false, false);
        assert_eq!(
            editor.cursor.line, 0,
            "first keystroke after dashboard open must not adopt an owner-write \
             EOF position; landed on line {}",
            editor.cursor.line
        );
    }
}
